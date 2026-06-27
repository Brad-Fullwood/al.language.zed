xmlport 50122 "Customer Export"
{
    Format = Xml;
    Direction = Export;

    schema
    {
        textelement(Root)
        {
            tableelement(Customer; "Test Customer")
            {
                fieldelement(No; Customer."No.") { }
                fieldelement(CustName; Customer.Name) { }
            }
        }
    }
}
